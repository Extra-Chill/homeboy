use homeboy_core::component::{self, Component};
use homeboy_core::error::{Error, ErrorCode, Result};
use homeboy_core::{self};
use homeboy_extension_contract::api::v1::{
    ExtensionApiCatalogEntryStatus, ExtensionApiCatalogRequest, ACTION_CAPABILITY_PREFIX,
    EXTENSION_API_CATALOG_REQUEST_SCHEMA, EXTENSION_API_V1,
};
use homeboy_extension_contract::manifest_capability_config::ReleasePreflightConfig;
use homeboy_extension_contract::HookEvent;
use std::collections::BTreeSet;

use super::types::{ReleaseOptions, ReleaseReadinessProvenance};

/// Load a component with portable config fallback when path_override is set.
/// In CI environments, the component may not be registered — only homeboy.json exists.
pub(crate) fn load_component(component_id: &str, options: &ReleaseOptions) -> Result<Component> {
    component::resolve_effective(Some(component_id), options.path_override.as_deref(), None)
}

/// Release's bounded view of a configured extension.
///
/// Action ownership comes only from the versioned Extension API descriptor.
/// Release-preflight declarations are local release policy, not public API data.
#[derive(Debug, Clone, Default)]
pub(super) struct ReleaseExtension {
    pub(super) id: String,
    action_ids: BTreeSet<String>,
    pub(super) release_preflights: Vec<ReleasePreflightConfig>,
    pub(super) post_release_hooks: Vec<String>,
}

impl ReleaseExtension {
    pub(super) fn provides_action(&self, action_id: &str) -> bool {
        self.action_ids.contains(action_id)
    }

    #[cfg(test)]
    pub(super) fn from_manifest(manifest: &homeboy_extension_contract::ExtensionManifest) -> Self {
        Self {
            id: manifest.id.clone(),
            action_ids: manifest
                .actions
                .iter()
                .map(|action| action.id.clone())
                .collect(),
            release_preflights: manifest.release_preflights.clone(),
            post_release_hooks: manifest
                .hooks
                .get(&HookEvent::PostRelease)
                .cloned()
                .unwrap_or_default(),
        }
    }
}

/// Resolve the component's declared extensions from one Extension API v1 catalog snapshot.
pub(super) fn resolve_extensions(component: &Component) -> Result<Vec<ReleaseExtension>> {
    let mut extensions = Vec::new();
    if let Some(configured) = component.extensions.as_ref() {
        let mut extension_ids: Vec<String> = configured.keys().cloned().collect();
        extension_ids.sort();
        let catalog = homeboy_core::extension::catalog::list_api(&ExtensionApiCatalogRequest {
            schema: EXTENSION_API_CATALOG_REQUEST_SCHEMA.to_string(),
            api_version: EXTENSION_API_V1,
        });
        if let Some(failure) = catalog.failure {
            return Err(Error::validation_invalid_argument(
                "extension_api",
                failure.message,
                None,
                None,
            ));
        }
        let suggestions: Vec<String> = catalog
            .entries
            .iter()
            .map(|entry| entry.id.clone())
            .collect();
        for extension_id in extension_ids {
            let entry = catalog
                .entries
                .iter()
                .find(|entry| entry.id == extension_id)
                .ok_or_else(|| {
                    Error::extension_not_found(extension_id.to_string(), suggestions.clone())
                })?;
            if entry.status != ExtensionApiCatalogEntryStatus::Available {
                return Err(Error::validation_invalid_argument(
                    "extensions",
                    format!(
                        "Configured extension '{}' is not available through Extension API v1",
                        extension_id
                    ),
                    Some(extension_id),
                    None,
                ));
            }
            let descriptor = entry.descriptor.as_ref().ok_or_else(|| {
                Error::internal_unexpected(format!(
                    "Extension API v1 catalog entry '{}' is missing its descriptor",
                    entry.id
                ))
            })?;
            // Preflight metadata is intentionally kept out of the public catalog.
            let manifest = homeboy_core::extension::catalog::load_extension(&entry.id)?;
            extensions.push(ReleaseExtension {
                id: descriptor.identity.id.clone(),
                action_ids: descriptor
                    .capabilities
                    .iter()
                    .filter_map(|capability| capability.id.strip_prefix(ACTION_CAPABILITY_PREFIX))
                    .map(str::to_string)
                    .collect(),
                release_preflights: manifest.release_preflights,
                post_release_hooks: manifest
                    .hooks
                    .get(&HookEvent::PostRelease)
                    .cloned()
                    .unwrap_or_default(),
            });
        }
    }
    Ok(extensions)
}

/// Capture immutable identities from the dependency adapters and materialized
/// extension manifests in the process that executed a portable review gate.
pub fn readiness_provenance(component: &Component) -> Result<ReleaseReadinessProvenance> {
    let mut provenance = ReleaseReadinessProvenance::default();
    let dependencies =
        match homeboy_core::deps::status(Some(&component.id), Some(&component.local_path), None) {
            Ok(status) => status.packages,
            // Dependency hydration treats a component with no declared provider as
            // a no-op; provenance must preserve that same contract.
            Err(error)
                if error.code == ErrorCode::ValidationInvalidArgument
                    && error.details.get("field").and_then(|value| value.as_str())
                        == Some("dependency_provider") =>
            {
                Vec::new()
            }
            Err(error) => return Err(error),
        };
    for dependency in dependencies {
        if let Some(revision) = dependency.locked_reference.or(dependency.locked_version) {
            provenance.dependencies.insert(dependency.name, revision);
        }
    }
    for extension in resolve_extensions(component)? {
        // Installation paths remain private to provenance hashing.
        let manifest = homeboy_core::extension::catalog::load_extension(&extension.id)?;
        let path = manifest.extension_path.as_deref().ok_or_else(|| {
            Error::internal_unexpected(format!(
                "resolved extension '{}' has no materialized path",
                manifest.id
            ))
        })?;
        let manifest_path = std::path::Path::new(path).join(format!("{}.json", manifest.id));
        let revision = homeboy_engine_primitives::content_hash::sha256_file(&manifest_path)
            .map_err(|error| {
                Error::internal_io(error.to_string(), Some(manifest_path.display().to_string()))
            })?;
        provenance
            .extensions
            .insert(extension.id, format!("sha256:{revision}"));
    }
    Ok(provenance)
}

#[cfg(test)]
mod tests {
    use super::{load_component, resolve_extensions};
    use crate::release::types::ReleaseOptions;
    use homeboy_core::component::{Component, ScopedExtensionConfig};
    use homeboy_extension_contract::ExtensionManifest;

    #[test]
    fn test_load_component() {
        let temp = tempfile::tempdir().expect("tempdir");
        let homeboy_json = temp.path().join("homeboy.json");
        std::fs::write(
            homeboy_json,
            r#"{
                "id": "fixture",
                "components": {
                    "fixture": {
                        "type": "fixture-type",
                        "path": "."
                    }
                }
            }"#,
        )
        .expect("write homeboy config");

        let component = load_component(
            "fixture",
            &ReleaseOptions {
                path_override: Some(temp.path().to_string_lossy().to_string()),
                ..Default::default()
            },
        )
        .expect("component should load from path override");

        assert_eq!(component.id, "fixture");
    }

    #[test]
    fn load_component_rejects_missing_path_override() {
        let temp = tempfile::tempdir().expect("tempdir");
        let missing = temp.path().join("missing-component");

        let error = load_component(
            "fixture",
            &ReleaseOptions {
                path_override: Some(missing.to_string_lossy().to_string()),
                ..Default::default()
            },
        )
        .expect_err("release path overrides must exist");

        assert_eq!(error.code.as_str(), "validation.invalid_argument");
        assert!(error.message.contains("--path override does not exist"));
    }

    #[test]
    fn test_resolve_extensions() {
        let component = Component::default();

        let extensions = resolve_extensions(&component).expect("missing extensions are optional");

        assert!(extensions.is_empty());
    }

    #[test]
    fn resolve_extensions_derives_actions_from_the_api_catalog() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let mut manifest: ExtensionManifest = serde_json::from_value(serde_json::json!({
                "name": "Registry",
                "version": "1.0.0",
                "actions": [{
                    "id": "release.publish",
                    "label": "Publish release",
                    "type": "command",
                    "command": "true"
                }],
                "release_preflights": [{
                    "id": "publish_token",
                    "label": "Validate publish token",
                    "action": "release.preflight.publish-token"
                }]
            }))
            .expect("extension manifest");
            manifest.id = "registry".to_string();
            homeboy_core::extension::catalog::save_manifest(&manifest)
                .expect("save extension manifest");
            let component = Component {
                extensions: Some(std::collections::HashMap::from([(
                    "registry".to_string(),
                    ScopedExtensionConfig::default(),
                )])),
                ..Component::default()
            };

            let extensions = resolve_extensions(&component).expect("resolve extensions");

            assert_eq!(extensions.len(), 1);
            assert!(extensions[0].provides_action("release.publish"));
            assert!(!extensions[0].provides_action("release.package"));
            assert_eq!(extensions[0].release_preflights.len(), 1);
        });
    }
}
