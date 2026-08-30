use homeboy_core::error::{Error, Result};
use homeboy_extension_contract::sidecar_config::{
    StructuredSidecarContract, StructuredSidecarDeclaration,
};
use homeboy_extension_contract::{
    DeploymentProviderLayeredInputManifest, DeploymentProviderManifest, ExtensionManifest,
};

/// Deployment providers declared by an extension manifest.
pub fn deployment_providers(manifest: &ExtensionManifest) -> &[DeploymentProviderManifest] {
    &manifest.deployment_providers
}

pub fn deployment_provider_layered_input(
    extension_id: &str,
    provider_id: &str,
) -> Result<Option<DeploymentProviderLayeredInputManifest>> {
    let extension = crate::extension::catalog::load_extension(extension_id)?;
    let provider = deployment_providers(&extension)
        .iter()
        .find(|provider| provider.id == provider_id)
        .ok_or_else(|| Error::validation_invalid_argument(
            "deployment_provider.provider",
            format!("Extension '{extension_id}' does not declare deployment provider '{provider_id}'"),
            None,
            None,
        ))?;
    Ok(provider.layered_input.clone())
}

#[cfg(test)]
mod deployment_provider_tests {
    use super::*;
    use homeboy_extension_contract::DEPLOYMENT_PROVIDER_PAYLOAD_SCHEMA;

    #[test]
    fn reads_multiple_generic_provider_descriptors() {
        let manifest: ExtensionManifest = serde_json::from_value(serde_json::json!({
            "name": "fixture", "version": "1.0.0",
            "deployment_providers": [
                { "id": "fixture.alpha", "command": "fixture-alpha --contract {{payload.contract}}" },
                { "id": "fixture.beta", "command": "fixture-beta --contract {{payload.contract}}", "dry_run_command": "fixture-beta --dry-run --contract {{payload.contract}}", "layered_input": { "schema": "homeboy/deployment-provider-payload/v1", "target_required": true, "result_schema": "fixture/deployment-result/v1" } }
            ]
        }))
        .expect("fixture manifest");

        let providers = deployment_providers(&manifest);
        assert_eq!(providers.len(), 2);
        assert_eq!(providers[0].id, "fixture.alpha");
        assert_eq!(providers[1].id, "fixture.beta");
        assert!(providers[0].dry_run_command.is_none());
        assert_eq!(
            providers[1].dry_run_command.as_deref(),
            Some("fixture-beta --dry-run --contract {{payload.contract}}")
        );
        assert_eq!(
            providers[1]
                .layered_input
                .as_ref()
                .expect("layered input")
                .schema,
            DEPLOYMENT_PROVIDER_PAYLOAD_SCHEMA
        );
        assert!(
            providers[1]
                .layered_input
                .as_ref()
                .expect("layered input")
                .target_required
        );
    }

    /// While `deployment_providers` rode in `ExtensionManifest::extra`, a
    /// malformed descriptor was `.ok()`-discarded and reported as "extension
    /// declares no providers" — indistinguishable from a correct manifest that
    /// declares none. As a typed field it is a named deserialization error.
    #[test]
    fn malformed_provider_descriptor_is_an_error_not_an_empty_list() {
        let result: std::result::Result<ExtensionManifest, _> =
            serde_json::from_value(serde_json::json!({
                "name": "fixture", "version": "1.0.0",
                "deployment_providers": [
                    { "id": "fixture.alpha" }
                ]
            }));

        let error = result.expect_err("a provider missing `command` must not deserialize");
        assert!(
            error.to_string().contains("command"),
            "error should name the missing field: {error}"
        );
    }
}

// Sidecar-declaration helpers depend on core run-dir constants, so they stay
// in core as free functions rather than moving with the manifest data model.
/// Structured sidecars this extension explicitly declares.
/// Missing declarations mean the extension has no structured sidecar
/// contract for that output.
pub fn structured_sidecars(manifest: &ExtensionManifest) -> Vec<StructuredSidecarDeclaration> {
    manifest
        .structured_sidecars
        .iter()
        .filter_map(|(name, contract)| {
            crate::extension::manifest_sidecar::structured_sidecar_declaration(contract, name)
        })
        .collect()
}

/// Schema version declared by the canonical `structured_sidecars` manifest
/// section for a logical sidecar name.
pub fn structured_sidecar_schema_version<'a>(
    manifest: &'a ExtensionManifest,
    name: &'a str,
) -> Option<&'a str> {
    manifest
        .structured_sidecars
        .get(name)
        .and_then(|contract| match contract {
            StructuredSidecarContract::Enabled(true) => {
                homeboy_core::structured_sidecar::default_schema_version(name)
            }
            StructuredSidecarContract::Enabled(false) => None,
            StructuredSidecarContract::Detail(detail) if detail.enabled => detail
                .schema_version
                .as_deref()
                .or_else(|| homeboy_core::structured_sidecar::default_schema_version(name)),
            StructuredSidecarContract::Detail(_) => None,
        })
}

#[cfg(test)]
mod tests {
    use homeboy_audit_contract::TestMappingConfig;
    use homeboy_extension_contract::notification_transport_config::NOTIFICATION_TRANSPORT_SCHEMA;
    use homeboy_extension_contract::NotificationTransportConfig;

    #[test]
    fn notification_transport_requires_versioned_literal_argv_contract() {
        let invalid = NotificationTransportConfig {
            schema: "wrong".to_string(),
            id: "test.run-completion".to_string(),
            command: vec!["true".to_string()],
            route_resolver: None,
        };
        assert!(invalid.validate().is_err());
        let invalid = NotificationTransportConfig {
            schema: NOTIFICATION_TRANSPORT_SCHEMA.to_string(),
            id: "bad id".to_string(),
            command: vec!["true".to_string()],
            route_resolver: None,
        };
        assert!(invalid.validate().is_err());
        let invalid = NotificationTransportConfig {
            schema: NOTIFICATION_TRANSPORT_SCHEMA.to_string(),
            id: "test.run-completion".to_string(),
            command: vec![],
            route_resolver: None,
        };
        assert!(invalid.validate().is_err());
    }

    #[test]
    fn effective_trivial_method_names_falls_back_to_builtin_set() {
        // No config-declared idiomatic names → core uses the builtin agnostic
        // set so existing behavior is preserved without the detector embedding
        // the literals.
        let config = TestMappingConfig::default();
        let names = config.effective_trivial_method_names();
        assert!(names.iter().any(|n| n == "len"));
        assert!(names.iter().any(|n| n == "__construct"));

        let prefixes = config.effective_trivial_method_prefixes();
        assert!(prefixes.iter().any(|p| p == "get_"));
        assert!(prefixes.iter().any(|p| p == "is_"));
    }

    #[test]
    fn effective_trivial_method_names_honors_configured_policy() {
        // A project/extension-declared policy fully replaces the builtin set —
        // language/ecosystem conventions live in config, not in core.
        let config = TestMappingConfig {
            trivial_method_names: vec!["only_this".to_string()],
            trivial_method_prefixes: vec!["fetch_".to_string()],
            ..Default::default()
        };

        let names = config.effective_trivial_method_names();
        assert_eq!(names, vec!["only_this".to_string()]);
        // Builtin literals are not silently merged in.
        assert!(!names.iter().any(|n| n == "len"));

        let prefixes = config.effective_trivial_method_prefixes();
        assert_eq!(prefixes, vec!["fetch_".to_string()]);
        assert!(!prefixes.iter().any(|p| p == "get_"));
    }
}
