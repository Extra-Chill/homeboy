//! `homeboy/control-plane-capabilities/v1` — which resources this build serves.

use serde::{Deserialize, Serialize};

pub const CONTROL_PLANE_CAPABILITIES_SCHEMA: &str = "homeboy/control-plane-capabilities/v1";
pub const LEGACY_COMPATIBILITY_MINOR_VERSIONS: u32 = 1;

/// Pure serializable declaration of control-plane resources and compatibility.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ControlPlaneCapabilities {
    pub schema: String,
    pub resources: Vec<ControlPlaneResource>,
    pub compatibility: CompatibilityWindow,
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
            resources: vec![
                ControlPlaneResource::Mission,
                ControlPlaneResource::Run,
                ControlPlaneResource::Task,
                ControlPlaneResource::Attempt,
                ControlPlaneResource::Execution,
                ControlPlaneResource::ProviderSession,
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
        ControlPlaneCapabilities, ControlPlaneResource, CONTROL_PLANE_CAPABILITIES_SCHEMA,
        LEGACY_COMPATIBILITY_MINOR_VERSIONS,
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
        assert_eq!(
            value["resources"],
            serde_json::json!([
                "mission",
                "run",
                "task",
                "attempt",
                "execution",
                "provider_session"
            ])
        );
        let decoded: ControlPlaneCapabilities = serde_json::from_value(value).expect("deserialize");
        assert_eq!(decoded, document);
        assert_eq!(decoded.resources.len(), 6);
        assert!(decoded.resources.contains(&ControlPlaneResource::Mission));
    }
}
