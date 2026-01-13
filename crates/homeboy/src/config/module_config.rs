use serde_json::Value;
use std::collections::HashMap;

use crate::module::ModuleManifest;
use crate::module_settings::ModuleSettingsValidator;
use crate::{Error, Result};

use super::{ComponentConfiguration, ProjectConfiguration};

pub struct ModuleScope;

impl ModuleScope {
    pub fn effective_settings(
        module_id: &str,
        project: Option<&ProjectConfiguration>,
        component: Option<&ComponentConfiguration>,
    ) -> HashMap<String, Value> {
        let mut settings = HashMap::new();

        if let Some(project) = project {
            if let Some(project_modules) = project.scoped_modules.as_ref() {
                if let Some(project_config) = project_modules.get(module_id) {
                    settings.extend(project_config.settings.clone());
                }
            }
        }

        if let Some(component) = component {
            if let Some(component_modules) = component.scoped_modules.as_ref() {
                if let Some(component_config) = component_modules.get(module_id) {
                    settings.extend(component_config.settings.clone());
                }
            }
        }

        settings
    }

    pub fn effective_settings_validated(
        module: &ModuleManifest,
        project: Option<&ProjectConfiguration>,
        component: Option<&ComponentConfiguration>,
    ) -> Result<HashMap<String, Value>> {
        let module_id = module.id.as_str();

        let mut out: HashMap<String, Value> = HashMap::new();
        for setting in &module.settings {
            let Some(default_value) = setting.default.clone() else {
                continue;
            };

            out.insert(setting.id.clone(), default_value);
        }

        let project_settings = project
            .and_then(|p| p.scoped_modules.as_ref())
            .and_then(|m| m.get(module_id))
            .map(|c| c.settings.clone())
            .unwrap_or_default();
        let component_settings = component
            .and_then(|c| c.scoped_modules.as_ref())
            .and_then(|m| m.get(module_id))
            .map(|c| c.settings.clone())
            .unwrap_or_default();

        let validator = ModuleSettingsValidator::new(module);
        validator.validate_settings_map("project", &project_settings)?;
        validator.validate_settings_map("component", &component_settings)?;

        for settings in [&project_settings, &component_settings] {
            for (key, value) in settings {
                out.insert(key.clone(), value.clone());
            }
        }

        Ok(out)
    }

    pub fn validate_project_compatibility(
        module: &ModuleManifest,
        project: &ProjectConfiguration,
    ) -> Result<()> {
        let Some(requires) = module.requires.as_ref() else {
            return Ok(());
        };

        // Check required modules
        for required_module in &requires.modules {
            if !project.has_module(required_module) {
                return Err(Error::validation_invalid_argument(
                    "project.modules",
                    format!(
                        "Module '{}' requires module '{}', but project does not have it enabled",
                        module.id, required_module
                    ),
                    None,
                    None,
                ));
            }
        }

        // Check required components
        for required in &requires.components {
            if !project.component_ids.iter().any(|c| c == required) {
                return Err(Error::validation_invalid_argument(
                    "project.componentIds",
                    format!(
                        "Module '{}' requires component '{}', but project does not include it",
                        module.id, required
                    ),
                    None,
                    None,
                ));
            }
        }

        Ok(())
    }

    pub fn resolve_component_scope(
        module: &ModuleManifest,
        project: &ProjectConfiguration,
        component_id: Option<&str>,
    ) -> Result<Option<String>> {
        let required_components = module
            .requires
            .as_ref()
            .map(|r| &r.components)
            .filter(|c| !c.is_empty());

        let Some(required_components) = required_components else {
            return Ok(component_id.map(str::to_string));
        };

        let matching_component_ids: Vec<String> = required_components
            .iter()
            .filter(|required_id| project.component_ids.iter().any(|id| id == *required_id))
            .cloned()
            .collect();

        if matching_component_ids.is_empty() {
            return Err(Error::validation_invalid_argument(
                "project.componentIds",
                format!(
                    "Module '{}' requires components {:?}; none are configured for this project",
                    module.id, required_components
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
                        "Module '{}' only supports project components {:?}; --component '{}' is not compatible",
                        module.id, matching_component_ids, component_id
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
                "Module '{}' matches multiple project components {:?}; pass --component <id>",
                module.id, matching_component_ids
            ),
            None,
            None,
        ))
    }
}
