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
    /// A checkout-less GitHub-Release-asset component (#14782) has nothing to
    /// attach a path to, so checked-in project config may omit this key
    /// entirely rather than round-tripping an empty string (#14795).
    #[serde(default)]
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
    /// Components (by id, local directory name, or remote directory name) that
    /// must be applied before this one when they deploy in the same run.
    ///
    /// Extensions can also infer these (see the extension contract's
    /// `deploy.dependency_inference`); explicit entries are merged with any
    /// inferred ones. Dependencies outside the current deploy are ignored.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub depends_on: Option<Vec<String>>,
}

impl ProjectComponentAttachment {
    /// A provider is selected only by project-owned target configuration. A
    /// portable provider policy describes a capability; it does not choose the
    /// deployment route for every project that attaches the component.
    pub fn selects_deployment_provider(&self) -> bool {
        self.deployment_provider.is_some() || self.deployment_provider_input.is_some()
    }
}

pub type ProjectComponentOverrides = crate::component::ComponentOverrideConfig;

#[cfg(test)]
mod attachment_construction_tests {
    use super::ProjectComponentAttachment;

    /// A stored attachment written before optional fields existed must still
    /// load, and a checkout-less GitHub-Release-asset attachment (#14782) may
    /// omit `local_path` entirely (#14795).
    #[test]
    fn stored_attachments_with_only_an_id_still_deserialize() {
        for raw in [
            r#"{"id":"plugin","local_path":"/src/plugin"}"#,
            r#"{"id":"plugin"}"#,
        ] {
            let attachment: ProjectComponentAttachment =
                serde_json::from_str(raw).unwrap_or_else(|err| panic!("{raw}: {err}"));
            assert_eq!(attachment.id, "plugin");
            assert!(attachment.remote_path.is_none());
            assert!(attachment.deployment_provider_input.is_none());
        }
        let checkout_less: ProjectComponentAttachment =
            serde_json::from_str(r#"{"id":"plugin"}"#).expect("checkout-less attachment");
        assert_eq!(checkout_less.local_path, "");
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

    #[test]
    fn provider_route_requires_project_target_configuration() {
        let local = ProjectComponentAttachment {
            id: "fixture".to_string(),
            local_path: "/source/fixture".to_string(),
            ..Default::default()
        };
        let layered = ProjectComponentAttachment {
            deployment_provider_input: Some(serde_json::json!({ "target": "site" })),
            ..local.clone()
        };
        let legacy = ProjectComponentAttachment {
            deployment_provider: Some(crate::component::DeploymentProviderAttachment {
                extension: "fixture-extension".to_string(),
                provider: "fixture.deploy".to_string(),
                contract: Some("deploy-contract.json".to_string()),
                policy: None,
            }),
            ..local.clone()
        };

        assert!(!local.selects_deployment_provider());
        assert!(layered.selects_deployment_provider());
        assert!(legacy.selects_deployment_provider());
    }
}
