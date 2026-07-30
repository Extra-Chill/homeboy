use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ProjectComponentAttachment {
    pub id: String,
    pub local_path: String,
    /// Project-specific deploy target for this attached component.
    ///
    /// Repo-owned `homeboy.json` is portable component metadata, while the
    /// install path can vary by project layout. Keeping this optional field on
    /// the attachment lets one component deploy to multiple projects without
    /// rewriting the repo-tracked `remote_path` for each environment.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remote_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deployment_provider: Option<crate::component::DeploymentProviderAttachment>,
    /// Opaque project-owned input for a layered deployment provider.
    ///
    /// This deliberately belongs to the project attachment rather than the
    /// portable component model: target identity is environment-owned.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deployment_provider_input: Option<serde_json::Value>,
}

pub type ProjectComponentOverrides = crate::component::ComponentOverrideConfig;

#[cfg(test)]
mod tests {
    use super::ProjectComponentAttachment;

    #[test]
    fn provider_input_is_project_only() {
        let attachment: ProjectComponentAttachment = serde_json::from_value(serde_json::json!({
            "id": "fixture",
            "local_path": "/source/fixture",
            "deployment_provider_input": { "credential": "project-secret" }
        }))
        .expect("project attachment");

        assert_eq!(
            attachment.deployment_provider_input,
            Some(serde_json::json!({ "credential": "project-secret" }))
        );
        let portable: crate::component::Component = serde_json::from_value(
            serde_json::json!({ "id": "fixture", "deployment_provider_input": { "credential": "ignored" } }),
        )
        .expect("portable component");
        assert!(serde_json::to_value(portable)
            .expect("portable component serialization")
            .get("deployment_provider_input")
            .is_none());
    }
}
