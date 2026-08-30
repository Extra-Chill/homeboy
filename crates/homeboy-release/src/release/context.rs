use homeboy_core::component::{self, Component};
use homeboy_core::error::{Error, ErrorCode, Result};
use homeboy_core::extension;
use homeboy_core::{self, ExtensionManifest};

use super::types::{ReleaseOptions, ReleaseReadinessProvenance};

/// Load a component with portable config fallback when path_override is set.
/// In CI environments, the component may not be registered — only homeboy.json exists.
pub(crate) fn load_component(component_id: &str, options: &ReleaseOptions) -> Result<Component> {
    component::resolve_effective(Some(component_id), options.path_override.as_deref(), None)
}

/// Resolve the component's declared extensions for release dispatch.
pub(super) fn resolve_extensions(component: &Component) -> Result<Vec<ExtensionManifest>> {
    let mut extensions = Vec::new();
    if let Some(configured) = component.extensions.as_ref() {
        let mut extension_ids: Vec<String> = configured.keys().cloned().collect();
        extension_ids.sort();
        let suggestions = extension::available_extension_ids();
        for extension_id in extension_ids {
            let manifest = extension::load_extension(&extension_id).map_err(|_| {
                Error::extension_not_found(extension_id.to_string(), suggestions.clone())
            })?;
            extensions.push(manifest);
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
        let path = extension.extension_path.as_deref().ok_or_else(|| {
            Error::internal_unexpected(format!(
                "resolved extension '{}' has no materialized path",
                extension.id
            ))
        })?;
        let manifest = std::path::Path::new(path).join(format!("{}.json", extension.id));
        let revision =
            homeboy_engine_primitives::content_hash::sha256_file(&manifest).map_err(|error| {
                Error::internal_io(error.to_string(), Some(manifest.display().to_string()))
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
    use homeboy_core::component::Component;

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
}
