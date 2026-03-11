use crate::component::Component;
use crate::error::{Error, Result};
use crate::project::Project;
use std::collections::HashMap;

use super::load_extension;
use super::manifest::ExtensionManifest;

/// Settings resolution for extensions with project/component context.
pub struct ExtensionScope;

impl ExtensionScope {
    pub fn effective_settings(
        extension_id: &str,
        project: Option<&Project>,
        component: Option<&Component>,
    ) -> Result<HashMap<String, serde_json::Value>> {
        let mut settings = HashMap::new();

        if let Some(project) = project {
            if let Some(project_extensions) = project.extensions.as_ref() {
                if let Some(project_config) = project_extensions.get(extension_id) {
                    settings.extend(project_config.settings.clone());
                }
            }
        }

        if let Some(component) = component {
            if let Some(component_extensions) = component.extensions.as_ref() {
                if let Some(component_config) = component_extensions.get(extension_id) {
                    settings.extend(component_config.settings.clone());
                }
            }
        }

        Ok(settings)
    }

    pub fn validate_project_compatibility(
        extension: &ExtensionManifest,
        project: &Project,
    ) -> Result<()> {
        let Some(requires) = extension.requires.as_ref() else {
            return Ok(());
        };

        // Required extensions must be installed globally
        for required_extension in &requires.extensions {
            if load_extension(required_extension).is_err() {
                return Err(Error::validation_invalid_argument(
                    "extensions",
                    format!(
                        "Extension '{}' requires extension '{}', but it is not installed",
                        extension.id, required_extension
                    ),
                    None,
                    None,
                ));
            }
        }

        // Required components must be linked to the project
        for required in &requires.components {
            if !crate::project::has_component(project, required) {
                return Err(Error::validation_invalid_argument(
                    "project.componentIds",
                    format!(
                        "Extension '{}' requires component '{}', but project does not include it",
                        extension.id, required
                    ),
                    None,
                    None,
                ));
            }
        }

        Ok(())
    }

    pub fn resolve_component_scope(
        extension: &ExtensionManifest,
        project: &Project,
        component_id: Option<&str>,
    ) -> Result<Option<String>> {
        let required_components = extension
            .requires
            .as_ref()
            .map(|r| &r.components)
            .filter(|c| !c.is_empty());

        let Some(required_components) = required_components else {
            return Ok(component_id.map(str::to_string));
        };

        let matching_component_ids: Vec<String> = required_components
            .iter()
            .filter(|required_id| crate::project::has_component(project, required_id))
            .cloned()
            .collect();

        if matching_component_ids.is_empty() {
            return Err(Error::validation_invalid_argument(
                "project.componentIds",
                format!(
                    "Extension '{}' requires components {:?}; none are configured for this project",
                    extension.id, required_components
                ),
                None,
                None,
            ));
        }

        if let Some(component_id) = component_id {
            if !matching_component_ids.iter().any(|c| c == component_id) {
                return Err(Error::validation_invalid_argument(
                    "component",
                    format!(
                        "Extension '{}' only supports project components {:?}; --component '{}' is not compatible",
                        extension.id, matching_component_ids, component_id
                    ),
                    Some(component_id.to_string()),
                    None,
                ));
            }

            return Ok(Some(component_id.to_string()));
        }

        if matching_component_ids.len() == 1 {
            return Ok(Some(matching_component_ids[0].clone()));
        }

        Err(Error::validation_invalid_argument(
            "component",
            format!(
                "Extension '{}' matches multiple project components {:?}; pass --component <id>",
                extension.id, matching_component_ids
            ),
            None,
            None,
        ))
    }
}
