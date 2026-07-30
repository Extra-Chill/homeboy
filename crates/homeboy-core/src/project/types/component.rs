use serde::{Deserialize, Serialize};

/// A component attached to a project.
///
/// # Construction
///
/// Build these with `..Default::default()` and set only the fields you mean:
///
/// ```ignore
/// ProjectComponentAttachment {
///     id: component_id,
///     local_path: path,
///     remote_path: Some(remote),
///     ..Default::default()
/// }
/// ```
///
/// Every field beyond `id` and `local_path` is optional environment-owned
/// metadata, and this struct is constructed at ~28 sites across four crates.
/// Listing every field exhaustively makes each added field a repo-wide edit
/// that two concurrent PRs can each miss while both look green in isolation —
/// which broke the build twice on 2026-07-30 (#10799 for `deployment_provider`,
/// #10938 for `deployment_provider_input`, the latter in production code).
///
/// New fields must therefore be `Option<_>` with `#[serde(default)]`, so an
/// existing attachment on disk still deserializes and no call site has to
/// change.
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
mod attachment_construction_tests {
    use super::ProjectComponentAttachment;

    /// Adding an optional field must not require touching any call site.
    ///
    /// This is the contract that broke twice in one day. If a new field is not
    /// `Option`-with-a-`Default`, this stops compiling and the author learns it
    /// here rather than from a red build on an unrelated PR.
    #[test]
    fn an_attachment_is_constructible_from_identity_alone() {
        let attachment = ProjectComponentAttachment {
            id: "plugin".to_string(),
            local_path: "/src/plugin".to_string(),
            ..Default::default()
        };

        assert_eq!(attachment.id, "plugin");
        assert_eq!(attachment.local_path, "/src/plugin");
        assert!(
            attachment.remote_path.is_none()
                && attachment.deployment_provider.is_none()
                && attachment.deployment_provider_input.is_none(),
            "every field beyond identity must default to absent, so `..Default::default()` \
             is always a safe construction"
        );
    }

    /// A stored attachment written before a field existed must still load.
    #[test]
    fn an_attachment_missing_optional_fields_still_deserializes() {
        let attachment: ProjectComponentAttachment =
            serde_json::from_str(r#"{"id":"plugin","local_path":"/src/plugin"}"#)
                .expect("identity-only attachment must deserialize");

        assert_eq!(attachment.id, "plugin");
        assert!(attachment.deployment_provider_input.is_none());
    }
}

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
