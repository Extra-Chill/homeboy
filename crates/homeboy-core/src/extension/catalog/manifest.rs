use homeboy_extension_contract::sidecar_config::{
    StructuredSidecarContract, StructuredSidecarDeclaration,
};
use homeboy_extension_contract::ExtensionManifest;

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
}
