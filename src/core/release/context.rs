use crate::core::component::{self, Component};
use crate::core::error::{Error, Result};
use crate::core::extension::{self, ExtensionManifest};

use super::types::ReleaseOptions;

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

#[cfg(test)]
mod tests {
    use super::{load_component, resolve_extensions};
    use crate::core::component::Component;
    use crate::core::release::types::ReleaseOptions;

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
    fn test_resolve_extensions() {
        let component = Component::default();

        let extensions = resolve_extensions(&component).expect("missing extensions are optional");

        assert!(extensions.is_empty());
    }
}
